//! The small IOKit/CoreFoundation boundary used only on a monitor's run loop.
#![allow(non_snake_case)]

use objc2_core_foundation::CFString;
use std::{ffi::c_void, ptr};

pub(super) type Ref = *const c_void;
pub(super) type Callback = unsafe extern "C" fn(*mut c_void, i32, Ref, Ref);

#[link(name = "IOKit", kind = "framework")]
unsafe extern "C" {
    pub fn IOHIDCheckAccess(request: u32) -> u32;
    pub fn IOHIDRequestAccess(request: u32) -> bool;
    pub fn IOHIDManagerCreate(allocator: *const c_void, options: u32) -> *mut c_void;
    pub fn IOHIDManagerSetDeviceMatchingMultiple(manager: Ref, matching: Ref);
    pub fn IOHIDManagerCopyDevices(manager: *mut c_void) -> *mut c_void;
    pub fn IOHIDManagerRegisterDeviceMatchingCallback(
        manager: Ref,
        callback: Option<Callback>,
        context: *mut c_void,
    );
    pub fn IOHIDManagerRegisterDeviceRemovalCallback(
        manager: Ref,
        callback: Option<Callback>,
        context: *mut c_void,
    );
    pub fn IOHIDManagerScheduleWithRunLoop(manager: Ref, run_loop: Ref, mode: Ref);
    pub fn IOHIDManagerUnscheduleFromRunLoop(manager: Ref, run_loop: Ref, mode: Ref);
    pub fn IOHIDDeviceOpen(device: Ref, options: u32) -> i32;
    pub fn IOHIDDeviceClose(device: Ref, options: u32) -> i32;
    pub fn IOHIDDeviceConformsTo(device: Ref, page: u32, usage: u32) -> bool;
    pub fn IOHIDDeviceGetProperty(device: Ref, key: Ref) -> Ref;
    pub fn IOHIDDeviceGetService(device: Ref) -> u32;
    pub fn IOHIDDeviceCopyMatchingElements(
        device: *mut c_void,
        matching: *const c_void,
        options: u32,
    ) -> *mut c_void;
    pub fn IOHIDDeviceGetValue(device: Ref, element: Ref, value: *mut Ref) -> i32;
    pub fn IOHIDDeviceSetInputValueMatching(device: Ref, matching: Ref);
    pub fn IOHIDDeviceRegisterInputValueCallback(
        device: Ref,
        callback: Option<Callback>,
        context: *mut c_void,
    );
    pub fn IOHIDDeviceScheduleWithRunLoop(device: Ref, run_loop: Ref, mode: Ref);
    pub fn IOHIDDeviceUnscheduleFromRunLoop(device: Ref, run_loop: Ref, mode: Ref);
    pub fn IOHIDElementGetType(element: Ref) -> u32;
    pub fn IOHIDElementGetUsagePage(element: *mut c_void) -> u32;
    pub fn IOHIDElementGetUsage(element: *mut c_void) -> u32;
    pub fn IOHIDElementGetCookie(element: Ref) -> u32;
    pub fn IOHIDElementGetDevice(element: Ref) -> Ref;
    pub fn IOHIDValueGetElement(value: Ref) -> Ref;
    pub fn IOHIDValueGetIntegerValue(value: Ref) -> isize;
    pub fn IOHIDValueGetTimeStamp(value: Ref) -> u64;
    pub fn IORegistryEntryGetRegistryEntryID(service: u32, id: *mut u64) -> i32;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    pub fn CFRetain(value: Ref) -> Ref;
    pub fn CFRelease(value: Ref);
    pub fn CFEqual(left: Ref, right: Ref) -> u8;
    pub fn CFGetTypeID(value: Ref) -> usize;
    pub fn CFStringGetTypeID() -> usize;
    pub fn CFNumberGetTypeID() -> usize;
    pub fn CFNumberCreate(allocator: Ref, kind: isize, value: Ref) -> Ref;
    pub fn CFNumberGetValue(number: Ref, kind: isize, value: *mut c_void) -> u8;
    pub fn CFBooleanGetTypeID() -> usize;
    pub fn CFBooleanGetValue(value: Ref) -> u8;
    pub fn CFDictionaryCreateMutable(
        allocator: Ref,
        capacity: isize,
        keys: Ref,
        values: Ref,
    ) -> Ref;
    pub fn CFDictionarySetValue(dictionary: Ref, key: Ref, value: Ref);
    pub fn CFArrayCreateMutable(allocator: Ref, capacity: isize, callbacks: Ref) -> Ref;
    pub fn CFArrayAppendValue(array: Ref, value: Ref);
    pub fn CFArrayGetCount(array: Ref) -> isize;
    pub fn CFArrayGetValueAtIndex(array: Ref, index: isize) -> Ref;
    pub fn CFSetGetCount(set: Ref) -> isize;
    pub fn CFSetGetValues(set: Ref, values: *mut Ref);
    pub static kCFTypeDictionaryKeyCallBacks: u8;
    pub static kCFTypeDictionaryValueCallBacks: u8;
    pub static kCFTypeArrayCallBacks: u8;
}

pub(super) struct Owned(pub Ref);

impl Drop for Owned {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: Owned wraps only create/copy/retain results.
            unsafe { CFRelease(self.0) };
        }
    }
}

pub(super) fn dictionary(entries: &[(&str, i64)]) -> Owned {
    // SAFETY: standard CF callbacks retain the keys and values we insert.
    unsafe {
        let dict = Owned(CFDictionaryCreateMutable(
            ptr::null(),
            0,
            (&raw const kCFTypeDictionaryKeyCallBacks).cast(),
            (&raw const kCFTypeDictionaryValueCallBacks).cast(),
        ));
        for (key, value) in entries {
            let key = CFString::from_str(key);
            let number = Owned(CFNumberCreate(ptr::null(), 4, (value as *const i64).cast()));
            CFDictionarySetValue(dict.0, (&*key as *const CFString).cast(), number.0);
        }
        dict
    }
}

pub(super) fn property(device: Ref, key: &str) -> Ref {
    let key = CFString::from_str(key);
    // SAFETY: a live device borrows its property values; key lives through the call.
    unsafe { IOHIDDeviceGetProperty(device, (&*key as *const CFString).cast()) }
}

pub(super) fn number(device: Ref, key: &str) -> Option<u16> {
    let value = property(device, key);
    let mut number = 0_i64;
    // SAFETY: check CF type before reading an Int64 into valid storage.
    unsafe {
        if value.is_null()
            || CFGetTypeID(value) != CFNumberGetTypeID()
            || CFNumberGetValue(value, 4, (&raw mut number).cast()) == 0
        {
            return None;
        }
    }
    u16::try_from(number).ok()
}

pub(super) fn string(device: Ref, key: &str) -> Option<String> {
    let value = property(device, key);
    // SAFETY: type checked borrowed CFString lives with the device.
    unsafe {
        if value.is_null() || CFGetTypeID(value) != CFStringGetTypeID() {
            return None;
        }
        Some((&*value.cast::<CFString>()).to_string())
    }
}

pub(super) fn boolean(device: Ref, key: &str) -> bool {
    let value = property(device, key);
    // SAFETY: check the borrowed property's type before reading it.
    unsafe {
        if value.is_null() {
            return false;
        }
        if CFGetTypeID(value) == CFBooleanGetTypeID() {
            return CFBooleanGetValue(value) != 0;
        }
        let mut flag = 0_i64;
        CFGetTypeID(value) == CFNumberGetTypeID()
            && CFNumberGetValue(value, 4, (&raw mut flag).cast()) != 0
            && flag != 0
    }
}
