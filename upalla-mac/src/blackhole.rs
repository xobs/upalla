//! Detection of *other* processes capturing from the BlackHole device.
//!
//! Upalla writes denoised mic audio into BlackHole so that conferencing apps can
//! pick it up. Keeping that chain running unconditionally makes macOS report
//! Upalla as recording all the time, so the audio engine only runs the recording
//! chain while something else is actually listening on BlackHole.
//!
//! Per-process audio state is exposed through the `AudioProcessObject` API
//! (`kAudioHardwarePropertyProcessObjectList`), which is macOS 14.4+. On older
//! systems every query here returns `None`, meaning "unknown".

use std::ffi::c_void;
use std::ptr;

use objc2_core_foundation::{CFRetained, CFString};

type AudioObjectID = u32;
type OSStatus = i32;

const SYSTEM_OBJECT: AudioObjectID = 1; // kAudioObjectSystemObject
const SCOPE_GLOBAL: u32 = fourcc(b"glob"); // kAudioObjectPropertyScopeGlobal
const SCOPE_INPUT: u32 = fourcc(b"inpt"); // kAudioObjectPropertyScopeInput
const ELEMENT_MAIN: u32 = 0; // kAudioObjectPropertyElementMain

const PROP_DEVICES: u32 = fourcc(b"dev#"); // kAudioHardwarePropertyDevices
const PROP_NAME: u32 = fourcc(b"lnam"); // kAudioObjectPropertyName
const PROP_PROCESS_LIST: u32 = fourcc(b"prs#"); // kAudioHardwarePropertyProcessObjectList
const PROP_PROCESS_PID: u32 = fourcc(b"ppid"); // kAudioProcessPropertyPID
const PROP_PROCESS_RUNNING_INPUT: u32 = fourcc(b"piri"); // kAudioProcessPropertyIsRunningInput
const PROP_PROCESS_RUNNING_OUTPUT: u32 = fourcc(b"piro"); // kAudioProcessPropertyIsRunningOutput
const PROP_PROCESS_DEVICES: u32 = fourcc(b"pdv#"); // kAudioProcessPropertyDevices
const SCOPE_OUTPUT: u32 = fourcc(b"outp"); // kAudioObjectPropertyScopeOutput

const fn fourcc(code: &[u8; 4]) -> u32 {
    u32::from_be_bytes(*code)
}

#[repr(C)]
#[derive(Clone, Copy)]
struct AudioObjectPropertyAddress {
    selector: u32,
    scope: u32,
    element: u32,
}

impl AudioObjectPropertyAddress {
    fn new(selector: u32, scope: u32) -> Self {
        AudioObjectPropertyAddress {
            selector,
            scope,
            element: ELEMENT_MAIN,
        }
    }
}

#[link(name = "CoreAudio", kind = "framework")]
extern "C" {
    fn AudioObjectGetPropertyDataSize(
        object: AudioObjectID,
        address: *const AudioObjectPropertyAddress,
        qualifier_size: u32,
        qualifier: *const c_void,
        out_size: *mut u32,
    ) -> OSStatus;

    fn AudioObjectGetPropertyData(
        object: AudioObjectID,
        address: *const AudioObjectPropertyAddress,
        qualifier_size: u32,
        qualifier: *const c_void,
        io_size: *mut u32,
        out_data: *mut c_void,
    ) -> OSStatus;
}

/// Reads an array-valued property. `None` if the property is unsupported.
fn get_object_list(
    object: AudioObjectID,
    address: &AudioObjectPropertyAddress,
) -> Option<Vec<AudioObjectID>> {
    let mut size: u32 = 0;
    let status =
        unsafe { AudioObjectGetPropertyDataSize(object, address, 0, ptr::null(), &mut size) };
    if status != 0 {
        return None;
    }
    let count = size as usize / std::mem::size_of::<AudioObjectID>();
    if count == 0 {
        return Some(Vec::new());
    }
    let mut ids = vec![0u32; count];
    let status = unsafe {
        AudioObjectGetPropertyData(
            object,
            address,
            0,
            ptr::null(),
            &mut size,
            ids.as_mut_ptr().cast(),
        )
    };
    if status != 0 {
        return None;
    }
    ids.truncate(size as usize / std::mem::size_of::<AudioObjectID>());
    Some(ids)
}

fn get_u32(object: AudioObjectID, address: &AudioObjectPropertyAddress) -> Option<u32> {
    let mut value: u32 = 0;
    let mut size = std::mem::size_of::<u32>() as u32;
    let status = unsafe {
        AudioObjectGetPropertyData(
            object,
            address,
            0,
            ptr::null(),
            &mut size,
            (&mut value as *mut u32).cast(),
        )
    };
    (status == 0).then_some(value)
}

fn get_name(object: AudioObjectID) -> Option<String> {
    let address = AudioObjectPropertyAddress::new(PROP_NAME, SCOPE_GLOBAL);
    let mut cf: *const CFString = ptr::null();
    let mut size = std::mem::size_of::<*const CFString>() as u32;
    let status = unsafe {
        AudioObjectGetPropertyData(
            object,
            &address,
            0,
            ptr::null(),
            &mut size,
            (&mut cf as *mut *const CFString).cast(),
        )
    };
    if status != 0 || cf.is_null() {
        return None;
    }
    // CoreAudio hands back a +1 reference for CFString-valued properties.
    let name = unsafe { CFRetained::from_raw(ptr::NonNull::new(cf.cast_mut())?) };
    Some(name.to_string())
}

/// The `AudioObjectID` of the device with this exact name.
///
/// Matched by name rather than "first BlackHole" so that a setup with several
/// BlackHole devices resolves each one to the right object.
pub fn find_device_by_name(name: &str) -> Option<AudioObjectID> {
    let address = AudioObjectPropertyAddress::new(PROP_DEVICES, SCOPE_GLOBAL);
    let devices = get_object_list(SYSTEM_OBJECT, &address)?;
    devices
        .into_iter()
        .find(|&id| get_name(id).as_deref() == Some(name))
}

/// Whether the process-object API used by [`has_external_user`] is available
/// (macOS 14.4+). When it is not, the chains have to be driven manually.
pub fn detection_supported() -> bool {
    let address = AudioObjectPropertyAddress::new(PROP_PROCESS_LIST, SCOPE_GLOBAL);
    get_object_list(SYSTEM_OBJECT, &address).is_some()
}

/// Which side of a device another process is using.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Direction {
    /// Reading from the device — for BlackHole, an app using it as a microphone.
    Capture,
    /// Writing to the device — for BlackHole, an app using it as a speaker.
    Playback,
}

impl Direction {
    fn running_selector(self) -> u32 {
        match self {
            Direction::Capture => PROP_PROCESS_RUNNING_INPUT,
            Direction::Playback => PROP_PROCESS_RUNNING_OUTPUT,
        }
    }

    fn scope(self) -> u32 {
        match self {
            Direction::Capture => SCOPE_INPUT,
            Direction::Playback => SCOPE_OUTPUT,
        }
    }
}

/// `Some(true)` if a process other than this one is using `device` in
/// `direction`, `None` if that cannot be determined on this system.
pub fn has_external_user(device: AudioObjectID, direction: Direction) -> Option<bool> {
    let address = AudioObjectPropertyAddress::new(PROP_PROCESS_LIST, SCOPE_GLOBAL);
    let processes = get_object_list(SYSTEM_OBJECT, &address)?;
    let own_pid = std::process::id();

    for process in processes {
        let pid_address = AudioObjectPropertyAddress::new(PROP_PROCESS_PID, SCOPE_GLOBAL);
        if get_u32(process, &pid_address) == Some(own_pid) {
            continue;
        }
        let running_address =
            AudioObjectPropertyAddress::new(direction.running_selector(), SCOPE_GLOBAL);
        if get_u32(process, &running_address).unwrap_or(0) == 0 {
            continue;
        }
        let devices_address =
            AudioObjectPropertyAddress::new(PROP_PROCESS_DEVICES, direction.scope());
        if let Some(devices) = get_object_list(process, &devices_address) {
            if devices.contains(&device) {
                return Some(true);
            }
        }
    }
    Some(false)
}
