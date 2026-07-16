//! NRController raw-IMU reader (`libnr_loader.so`) — the sensor source for the phone-as-3D-pointer
//! (docs/plans/input-plan.md Phase C). Signatures RE-confirmed by disassembly (codex, 2026-07-14).
//!
//! Findings on this host (XREAL One Pro + phone): the controller registers as a touch+button
//! "handset" (`handheld_type=2`) and its **raw IMU is live** (accelerometer / gyroscope /
//! magnetometer), but the fused `NRControllerGetPose` never returns a real orientation (identity or
//! not-ready), and Godot's own `Input.get_gyroscope()` etc. read all-zero. So the pointer orientation
//! is fused in GDScript (`demo/phone_pointer.gd`) from this accelerometer (gravity → pitch/roll) +
//! gyroscope (yaw).
//!
//! Flow (from the disassembly): `GroupCreate(&group)` → `GroupGetCount(group,&n)` →
//! `GroupGetControllerId(group,0,&id)` → `Create(id,&ctrl)` → `Start(ctrl)`, then per frame
//! `StateUpdate(ctrl,&state)` + `StateGet*(state, …)`. `NRControllerCreate` is `(int32 id, uint64*
//! out)` — the arity matters (a wrong signature corrupts memory and hangs the app). Loader stubs
//! guard on the dispatch table (return 1 before the NR runtime is up), so calling early is safe.

use libloading::Library;
use std::sync::Mutex;

type FnOutU64 = unsafe extern "C" fn(*mut u64) -> i32;
type FnHandleOutI32 = unsafe extern "C" fn(u64, *mut i32) -> i32;
type FnHandleIdxOutI32 = unsafe extern "C" fn(u64, i32, *mut i32) -> i32;
type FnCreate = unsafe extern "C" fn(i32, *mut u64) -> i32;
type FnOneHandle = unsafe extern "C" fn(u64) -> i32;
type FnStateUpdate = unsafe extern "C" fn(u64, *mut u64) -> i32;
type FnStateGetXy = unsafe extern "C" fn(u64, *mut [f32; 2]) -> i32;
type FnStateGetXyz = unsafe extern "C" fn(u64, *mut [f32; 3]) -> i32;

struct Controller {
    _lib: Library, // keep libnr_loader.so mapped for the fn-pointers' lifetime
    state_update: FnStateUpdate,
    state_destroy: Option<FnOneHandle>,
    button_state: Option<FnHandleOutI32>,
    touch_state: Option<FnHandleOutI32>,
    touch_pose: Option<FnStateGetXy>,
    accelerometer: Option<FnStateGetXyz>,
    gyroscope: Option<FnStateGetXyz>,
    magnetometer: Option<FnStateGetXyz>,
    handle: u64,
}

// SAFETY: the fn-pointers resolve into libnr_loader.so (kept mapped by `_lib`); `handle` is an opaque
// u64 owned by the NR runtime. Only touched under the Mutex, from the main thread.
unsafe impl Send for Controller {}

static CONTROLLER: Mutex<Option<Controller>> = Mutex::new(None);

/// Discover + create + start the controller and keep it alive for [`poll_raw`]. Returns a one-line
/// diagnostic (controller count / id / connected & handheld type).
pub fn start() -> String {
    let mut slot = CONTROLLER.lock().unwrap_or_else(|e| e.into_inner());
    if slot.is_some() {
        return "[xreal] controller already started".to_string();
    }
    unsafe {
        let lib = match Library::new("libnr_loader.so") {
            Ok(l) => l,
            Err(e) => return format!("[xreal] controller dlopen failed: {e}"),
        };
        macro_rules! sym {
            ($name:literal, $ty:ty) => {
                match lib.get::<$ty>(concat!($name, "\0").as_bytes()) {
                    Ok(f) => *f,
                    Err(e) => return format!("[xreal] controller dlsym {} failed: {e}", $name),
                }
            };
        }
        let group_create: FnOutU64 = sym!("NRControllerGroupCreate", FnOutU64);
        let group_get_count: FnHandleOutI32 = sym!("NRControllerGroupGetCount", FnHandleOutI32);
        let group_get_id: FnHandleIdxOutI32 =
            sym!("NRControllerGroupGetControllerId", FnHandleIdxOutI32);
        let create: FnCreate = sym!("NRControllerCreate", FnCreate);
        let start_fn: FnOneHandle = sym!("NRControllerStart", FnOneHandle);
        let state_update: FnStateUpdate = sym!("NRControllerStateUpdate", FnStateUpdate);
        let group_destroy = lib
            .get::<FnOneHandle>(b"NRControllerGroupDestroy\0")
            .ok()
            .map(|f| *f);
        let state_destroy = lib
            .get::<FnOneHandle>(b"NRControllerStateDestroy\0")
            .ok()
            .map(|f| *f);

        let mut group: u64 = 0;
        let gc = group_create(&mut group);
        if gc != 0 || group == 0 {
            return format!("[xreal] controller GroupCreate failed (result={gc})");
        }
        let mut count: i32 = -1;
        group_get_count(group, &mut count);
        if count <= 0 {
            if let Some(gd) = group_destroy {
                gd(group);
            }
            return format!("[xreal] no controller (count={count})");
        }
        let mut controller_id: i32 = -1;
        group_get_id(group, 0, &mut controller_id);
        let mut handle: u64 = 0;
        let cr = create(controller_id, &mut handle);
        let sr = if cr == 0 && handle != 0 {
            start_fn(handle)
        } else {
            -1
        };
        if let Some(gd) = group_destroy {
            gd(group); // the group was only needed to discover the id
        }
        if cr != 0 || handle == 0 {
            return format!("[xreal] controller Create failed (result={cr})");
        }

        // Characterize the controller (diagnostic only).
        let mut conn_type = -1;
        let mut hand_type = -1;
        if let Ok(f) = lib.get::<FnHandleOutI32>(b"NRControllerGetConnectedType\0") {
            f(handle, &mut conn_type);
        }
        if let Ok(f) = lib.get::<FnHandleOutI32>(b"NRControllerGetHandheldType\0") {
            f(handle, &mut hand_type);
        }

        let button_state = lib
            .get::<FnHandleOutI32>(b"NRControllerStateGetButtonState\0")
            .ok()
            .map(|f| *f);
        let touch_state = lib
            .get::<FnHandleOutI32>(b"NRControllerStateTouchState\0")
            .ok()
            .map(|f| *f);
        let touch_pose = lib
            .get::<FnStateGetXy>(b"NRControllerStateGetTouchPose\0")
            .ok()
            .map(|f| *f);
        let accelerometer = lib
            .get::<FnStateGetXyz>(b"NRControllerStateGetAccelerometer\0")
            .ok()
            .map(|f| *f);
        let gyroscope = lib
            .get::<FnStateGetXyz>(b"NRControllerStateGetGyroscope\0")
            .ok()
            .map(|f| *f);
        let magnetometer = lib
            .get::<FnStateGetXyz>(b"NRControllerStateGetMagnetometer\0")
            .ok()
            .map(|f| *f);

        *slot = Some(Controller {
            _lib: lib,
            state_update,
            state_destroy,
            button_state,
            touch_state,
            touch_pose,
            accelerometer,
            gyroscope,
            magnetometer,
            handle,
        });
        format!("[xreal] controller started: count={count} id={controller_id} start={sr} connected_type={conn_type} handheld_type={hand_type}")
    }
}

/// Raw controller state for one frame (the phone IMU + touch/buttons). `accel` is proper
/// acceleration (gravity dir = `-normalize(accel)`); `gyro` is angular rate (rad/s).
#[derive(Default)]
pub struct Raw {
    pub ok: bool,
    pub accel: [f32; 3],
    pub gyro: [f32; 3],
    pub mag: [f32; 3],
    pub touch: i32,
    pub touch_xy: [f32; 2],
    pub buttons: i32,
}

/// One `StateUpdate` + read of every live sensor. `ok=false` if the controller isn't started.
pub fn poll_raw() -> Raw {
    let slot = CONTROLLER.lock().unwrap_or_else(|e| e.into_inner());
    let Some(c) = slot.as_ref() else {
        return Raw::default();
    };
    unsafe {
        let mut state: u64 = 0;
        (c.state_update)(c.handle, &mut state);
        let mut r = Raw {
            ok: state != 0,
            ..Default::default()
        };
        if state != 0 {
            if let Some(f) = c.accelerometer {
                f(state, &mut r.accel);
            }
            if let Some(f) = c.gyroscope {
                f(state, &mut r.gyro);
            }
            if let Some(f) = c.magnetometer {
                f(state, &mut r.mag);
            }
            if let Some(f) = c.touch_state {
                f(state, &mut r.touch);
            }
            if let Some(f) = c.touch_pose {
                f(state, &mut r.touch_xy);
            }
            if let Some(f) = c.button_state {
                f(state, &mut r.buttons);
            }
            if let Some(sd) = c.state_destroy {
                sd(state);
            }
        }
        r
    }
}
