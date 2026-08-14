//! XREAL hand tracking, published as a Godot `XRHandTracker`.
//!
//! **Hardware-gated to the XREAL Air 2 Ultra**, which has the outward SLAM cameras and the
//! perception feature. The One Pro returns `IsHandTrackingSupported() == false` and produces no
//! data. See `docs/develop/plans/hand-tracking-plan.md`.
//!
//! ## Data path: approach 2, the SDK's own exported wrappers
//!
//! We call the plugin's exported hand wrappers, which use the SDK's internal `InputManager`
//! singleton, so **no NR session handle is needed**, unlike the raw `NRHand*` flat API, which takes
//! a session we do not hold. This mirrors how `EnableTearedFrameCount` and friends are called, and
//! it is exactly what the Unity SDK's `XREALHandSubSystem` does per frame:
//!
//! - `bool IsHandTrackingSupported()` (libXREALXRPlugin.so `0x47c08`, calling
//!   `InputManager::IsHandTrackingSupported`)
//! - `bool UpdateHandPose()` (`0x47fe4`, calling `InputManager::UpdateHandPose`), which refreshes
//!   both hands once per frame
//! - `bool GetHandJointsPose(int handType, HandJointsPose* out)` (`0x47ff4`, calling
//!   `InputManager::GetHandJointsPose`)
//!
//! `HandJointsPose` is an `int32 isTracked` followed by a `Pose[26]`, where each `Pose` is a
//! position xyz plus a rotation xyzw, 7 floats. The SDK has already converted the poses to **Unity**
//! space, and we convert Unity to Godot here: position `(x, y, -z)`, quaternion `(-x, -y, z, w)`.
//! The array arrives in **Unity `XRHandJointID` order**, `[0]=Wrist, [1]=Palm, [2..25]=fingers`,
//! while Godot's `XRHandTracker` uses `PALM=0, WRIST=1, [2..25]=fingers` with the same finger order,
//! so we swap the first two and leave the rest.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use godot::classes::xr_hand_tracker::HandJoint;
use godot::classes::xr_positional_tracker::TrackerHand;
use godot::classes::{INode, Node, XrHandTracker, XrServer, XrTracker};
use godot::prelude::*;

use libloading::Library;

/// One Unity-space joint pose as written by `GetHandJointsPose`, a Unity `Pose` of position then
/// rotation.
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct UnityPose {
    position: [f32; 3],
    rotation: [f32; 4],
}

/// The out-struct filled by `GetHandJointsPose(handType, &mut HandJointsPose)`.
///
/// This matches the SDK's C# `HandJointsPose` under default P/Invoke marshalling: a `bool` becomes
/// a 4-byte `BOOL`, then a by-value `Pose[26]`, since `SizeConst = XRHandJointID.EndMarker - 1 =
/// 26`.
#[repr(C)]
#[derive(Default)]
struct HandJointsPose {
    is_tracked: i32,
    joints: [UnityPose; 26],
}

type FnBool = unsafe extern "C" fn() -> bool;
type FnGetHandJointsPose = unsafe extern "C" fn(i32, *mut HandJointsPose) -> bool;

struct HandApi {
    _lib: Library, // keep libXREALXRPlugin.so mapped for the fn-pointers' lifetime
    is_supported: FnBool,
    update: FnBool,
    get_joints: FnGetHandJointsPose,
}

// SAFETY: the fn-pointers resolve into libXREALXRPlugin.so, which `_lib` keeps mapped, and the
// wrappers use the SDK's `InputManager` singleton and take no external state. They are touched
// only under the Mutex.
unsafe impl Send for HandApi {}

static HAND_API: Mutex<Option<HandApi>> = Mutex::new(None);

/// `dlopen` libXREALXRPlugin.so and resolve the three exported hand wrappers. It is idempotent and
/// returns a one-line diagnostic. Calling it before the SDK is up is safe, because the wrappers
/// no-op through the InputManager singleton.
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
        *slot = Some(HandApi {
            _lib: lib,
            is_supported,
            update,
            get_joints,
        });
        "loaded"
    }
}

/// `true` when the connected glasses support hand tracking, meaning an Air 2 Ultra. It is `false` on
/// the One Pro and before the SDK is up.
pub fn is_supported() -> bool {
    let mut slot = HAND_API.lock().unwrap_or_else(|e| e.into_inner());
    ensure_api_locked(&mut slot);
    slot.as_ref()
        .map(|a| unsafe { (a.is_supported)() })
        .unwrap_or(false)
}

/// One converted hand: `tracked`, plus 26 Godot-space joint transforms indexed by the Godot
/// `HandJoint` ordinal.
pub struct HandSnapshot {
    pub tracked: bool,
    /// `[godot_joint_ord] -> Transform3D` in Godot space. Index 0 is Palm, 1 is Wrist, 2..25 the
    /// fingers.
    pub joints: [Transform3D; 26],
}

/// Refresh both hands, once per frame, then read one hand. `hand_type` is 0 for left and 1 for
/// right, matching the SDK `HandType`. It returns `None` when the API is unavailable or
/// `GetHandJointsPose` fails.
///
/// Call `update_frame()` once before polling both hands, so `UpdateHandPose` runs a single time per
/// frame.
pub fn poll(hand_type: i32) -> Option<HandSnapshot> {
    let slot = HAND_API.lock().unwrap_or_else(|e| e.into_inner());
    let api = slot.as_ref()?;
    let mut raw = HandJointsPose::default();
    if !unsafe { (api.get_joints)(hand_type, &mut raw) } {
        return None;
    }
    let mut joints = [Transform3D::IDENTITY; 26];
    for (i, p) in raw.joints.iter().enumerate() {
        // Unity `XRHandJointID` order to Godot `HandJoint` order: swap Wrist(0) and Palm(1), and the
        // fingers already match.
        let godot_ord = match i {
            0 => 1, // Unity Wrist -> Godot WRIST
            1 => 0, // Unity Palm  -> Godot PALM
            n => n,
        };
        joints[godot_ord] = unity_pose_to_godot(p);
    }
    Some(HandSnapshot {
        tracked: raw.is_tracked != 0,
        joints,
    })
}

/// Refresh both hands for this frame. It returns `false` when the API is unavailable, or while hand
/// tracking is not enabled yet; [`ensure_enabled`] attempts the enable lazily.
pub fn update_frame() -> bool {
    ensure_enabled();
    let mut slot = HAND_API.lock().unwrap_or_else(|e| e.into_inner());
    ensure_api_locked(&mut slot);
    slot.as_ref()
        .map(|a| unsafe { (a.update)() })
        .unwrap_or(false)
}

// --- Enable path: RE'd internal plugin functions reached by `LIB_BASE + offset` ------------------------
//
// `UpdateHandPose` no-ops until hand tracking is enabled. The minimal enable is a single internal
// call, `NativePerception::SetHandTrackingEnabled(perception, true)` (`libXREALXRPlugin.so
// 0x97174`). The perception instance is `*(InputManager + 0x48)`, and InputManager comes from
// `TSingleton::GetInstance` (0x47a10). We must NOT poke `+0x290`, `+0x204` or `+0x24c`: those are
// the STOP path, and `+0x290` is a one-shot latch that makes UpdateHandPose return false once set.
// See docs/develop/plans/hand-tracking-plan.md, "Enable path RE 2026-07-16". Guard on perception, session
// and config readiness, and retry until they are up.

const OFF_GET_INPUT_MANAGER: usize = 0x47a10; // TSingleton<InputManager>::GetInstance()
const OFF_SET_HAND_TRACKING_ENABLED: usize = 0x97174; // NativePerception::SetHandTrackingEnabled(bool)
const IM_PERCEPTION_PTR: usize = 0x48; // InputManager + 0x48 = NativePerception*
const NP_STARTED: usize = 0x18; // NativePerception + 0x18 (non-zero once start succeeded)
const NP_SESSION: usize = 0x28; // NativePerception + 0x28 (NR session handle)
const NP_CONFIG: usize = 0x38; // NativePerception + 0x38 (NR config handle)

static HAND_ENABLED: AtomicBool = AtomicBool::new(false);

type FnGetInputManager = unsafe extern "C" fn() -> *mut u8;
type FnSetHandTrackingEnabled = unsafe extern "C" fn(*mut u8, bool);

/// Attempt the one-shot enable once the SDK's perception is up. It is idempotent and does nothing
/// after the first success. Calling it every frame is safe: it early-returns once enabled and
/// guards every pointer.
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
            return; // perception not fully brought up yet, so retry next frame
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

/// Convert a Unity-space `Pose` to a Godot `Transform3D`.
///
/// Unity is left-handed with +Z forward and Godot is right-handed with -Z forward, so Z is negated:
/// a position of `(x, y, -z)` and a quaternion of `(-x, -y, z, w)`. That is the canonical
/// conversion, the same one `ffi.rs::to_godot_quaternion` applies to planes, anchors and image
/// tracking.
///
/// It used to negate Y on top of that, giving `(x, -y, -z)` and `(x, -y, -z, w)`, on the stated
/// grounds that "this port's eye cameras render with an inverted Y, a pose handedness of
/// `(x,-y,z,w)` that the head rig and phone pointer compensate for the same way". Both of those
/// compensations are gone, both earlier in this branch: the head pose is now `(-x,-y,-z,w)` paired
/// with a mirrored eye image on every submission path, and the phone pointer's own flip turned out
/// to be leftover calibration. What was left here is the render-pipeline artifact that
/// `docs/develop/plans/coordinate-systems-notes.md` identified, and that the depth-mesh commit
/// removed from `mesh_block_to_dict` for the same reason.
///
/// Device-verified on an Air 2 Ultra (2026-08-14): with the negation gone the joints rise with the
/// hand and the palm faces the way the real one does. The One Pro cannot check this at all, since it
/// answers `IsHandTrackingSupported()==false`. The negation had been device-confirmed once ("without
/// it the hand rendered upside-down"), but through the mirrored display that made it necessary, so
/// that confirmation did not survive the mirror fix.
fn unity_pose_to_godot(p: &UnityPose) -> Transform3D {
    let pos = Vector3::new(p.position[0], p.position[1], -p.position[2]);
    let rot = Quaternion::new(-p.rotation[0], -p.rotation[1], p.rotation[2], p.rotation[3]);
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

/// Node that publishes XREAL hand tracking to Godot's `XRServer` as two `XRHandTracker`s,
/// `/user/hand_tracker/left` and `/user/hand_tracker/right`. Add it to the scene, and then an
/// `XRHandModifier3D` carrying the matching tracker name animates a hand skeleton, an `XRNode3D`
/// carrying it follows the palm and hides itself when the hand is not tracked, or GDScript reads
/// the trackers through `XRServer.get_tracker(...)`. Same wiring as on an OpenXR headset.
///
/// Hardware-gated to the Air 2 Ultra. Elsewhere the trackers still register, and simply report no
/// tracking data, so an app that shows hands only while they are tracked shows none at all.
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
        Self {
            base,
            left: None,
            right: None,
            registered: false,
            logged_first_tracked: false,
        }
    }

    fn ready(&mut self) {
        // Register the two hand trackers once. `process` updates them every frame, and a hand simply
        // reports `has_tracking_data = false` until the device sees it.
        let left = make_tracker("/user/hand_tracker/left", TrackerHand::LEFT);
        let right = make_tracker("/user/hand_tracker/right", TrackerHand::RIGHT);
        let mut server = XrServer::singleton();
        server.add_tracker(&left.clone().upcast::<XrTracker>());
        server.add_tracker(&right.clone().upcast::<XrTracker>());
        self.left = Some(left);
        self.right = Some(right);
        self.registered = true;
        godot_print!(
            "[xreal] XrealHandTracker: registered left/right hand trackers (supported={})",
            is_supported()
        );
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
            && (left.as_ref().is_some_and(|s| s.tracked)
                || right.as_ref().is_some_and(|s| s.tracked))
        {
            self.logged_first_tracked = true;
            godot_print!("[xreal] XrealHandTracker: first tracked hand, feeding XRHandTracker(s)");
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

/// Push one hand's snapshot into its `XrHandTracker`, or clear tracking when the hand is absent or
/// untracked.
///
/// The `"default"` pose carries the palm, which is what Godot's own OpenXR hand tracking publishes
/// there (`openxr_hand_tracking_extension.cpp`). `XRHandModifier3D` reads the joints, but
/// `XRNode3D` reads that pose - for its transform AND for `show_when_tracked`, which hides the node
/// when the pose is invalidated. Publishing it is therefore what lets the standard rig (an
/// `XRNode3D` holding a hand model whose skeleton carries an `XRHandModifier3D`) behave here
/// exactly as it does on an OpenXR headset: placed on the real hand while tracked, gone otherwise.
/// Without it such a rig stays hidden even on an Air 2 Ultra, because the pose never arrives.
fn feed_tracker(tracker: &mut Gd<XrHandTracker>, snapshot: Option<HandSnapshot>) {
    use godot::classes::xr_hand_tracker::HandTrackingSource;
    use godot::classes::xr_pose::TrackingConfidence;

    let flags = hand_joint_flags_all();
    match snapshot {
        Some(s) if s.tracked => {
            tracker.set_has_tracking_data(true);
            tracker.set_hand_tracking_source(HandTrackingSource::UNOBSTRUCTED);
            for (i, joint) in GODOT_JOINTS.iter().enumerate() {
                tracker.set_hand_joint_transform(*joint, s.joints[i]);
                tracker.set_hand_joint_flags(*joint, flags);
            }
            // GODOT_JOINTS[0] is the palm. The SDK gives us no velocities, and it reports a hand as
            // either tracked or not, so the confidence is the tracked case's HIGH.
            tracker.set_pose(
                "default",
                s.joints[0],
                Vector3::ZERO,
                Vector3::ZERO,
                TrackingConfidence::HIGH,
            );
        }
        _ => {
            tracker.set_has_tracking_data(false);
            // NOT_TRACKED covers both a hand the cameras cannot see and glasses without hand
            // tracking at all: from an app's side those are the same answer, and neither offers a
            // pose. `invalidate_pose` is what drives XRNode3D's show_when_tracked to hide.
            tracker.set_hand_tracking_source(HandTrackingSource::NOT_TRACKED);
            tracker.invalidate_pose("default");
        }
    }
}

/// Position and orientation are valid and tracked; the SDK gives us no velocities.
fn hand_joint_flags_all() -> godot::classes::xr_hand_tracker::HandJointFlags {
    use godot::classes::xr_hand_tracker::HandJointFlags;
    HandJointFlags::ORIENTATION_VALID
        | HandJointFlags::ORIENTATION_TRACKED
        | HandJointFlags::POSITION_VALID
        | HandJointFlags::POSITION_TRACKED
}
