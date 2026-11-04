//! Non-seizing observation of physical pointer button elements. Every open is
//! preceded by IOHIDCheckAccess; discovery itself never opens a device.

use super::{
    Access, MonitorStatus, access,
    ffi::*,
    policy::{ButtonState, Edge},
};
use crate::{app_settings::NativeMouseButton, audio::CaptureInstant};
use std::{collections::HashMap, ffi::c_void, ptr};

#[derive(Clone, Copy)]
pub(crate) enum Event {
    Button(Edge),
    Removed(u64),
}

struct Element {
    native: Owned,
    binding: NativeMouseButton,
    state: ButtonState,
}

struct Device {
    native: Owned,
    elements: HashMap<u32, Element>,
}

pub(crate) struct Monitor {
    manager: Owned,
    run_loop: Ref,
    devices: HashMap<u64, Device>,
    wanted: Option<Vec<NativeMouseButton>>,
    callback: Box<dyn FnMut(Event)>,
    pub status: MonitorStatus,
}

impl Monitor {
    pub fn start(
        wanted: Option<Vec<NativeMouseButton>>,
        callback: impl FnMut(Event) + 'static,
    ) -> Option<Box<Self>> {
        if access() != Access::Granted {
            return None;
        }
        // SAFETY: independent devices stops the manager from opening anything;
        // only the vetted devices in add() are opened, without seizing them.
        let manager = Owned(unsafe { IOHIDManagerCreate(ptr::null(), 0x8) });
        if manager.0.is_null() {
            return None;
        }
        let mut monitor = Box::new(Self {
            manager,
            run_loop: unsafe { super::super::CFRunLoopGetCurrent() },
            devices: HashMap::new(),
            wanted,
            callback: Box::new(callback),
            status: MonitorStatus::Listening { devices: 0 },
        });
        let context = (&raw mut *monitor).cast();
        // SAFETY: the Box stays at the same address until callbacks are removed on drop.
        unsafe {
            let matching = Owned(CFArrayCreateMutable(
                ptr::null(),
                0,
                (&raw const kCFTypeArrayCallBacks).cast(),
            ));
            for usage in [1, 2] {
                let dict = dictionary(&[("DeviceUsagePage", 1), ("DeviceUsage", usage)]);
                CFArrayAppendValue(matching.0, dict.0);
            }
            IOHIDManagerSetDeviceMatchingMultiple(monitor.manager.0, matching.0);
            IOHIDManagerRegisterDeviceMatchingCallback(monitor.manager.0, Some(added), context);
            IOHIDManagerRegisterDeviceRemovalCallback(monitor.manager.0, Some(removed), context);
            let discovered = Owned(IOHIDManagerCopyDevices(monitor.manager.0));
            if !discovered.0.is_null() {
                let mut devices = vec![ptr::null(); CFSetGetCount(discovered.0) as usize];
                CFSetGetValues(discovered.0, devices.as_mut_ptr());
                devices.into_iter().for_each(|device| monitor.add(device));
            }
            IOHIDManagerScheduleWithRunLoop(
                monitor.manager.0,
                monitor.run_loop,
                super::super::kCFRunLoopCommonModes,
            );
        }
        Some(monitor)
    }

    fn add(&mut self, native: Ref) {
        if access() != Access::Granted || !physical_pointer(native) {
            return;
        }
        let source = registry_id(native);
        if source == 0 || self.devices.contains_key(&source) {
            return;
        }
        let Some(vendor) = number(native, "VendorID") else {
            return;
        };
        let Some(product) = number(native, "ProductID") else {
            return;
        };
        if self.wanted.as_ref().is_some_and(|wanted| {
            !wanted
                .iter()
                .any(|binding| binding.vendor == vendor && binding.product == product)
        }) {
            return;
        }
        let matching = dictionary(&[("UsagePage", 9), ("UsageMin", 3), ("UsageMax", 65535)]);
        // SAFETY: native is a live discovery result; matching includes only Button-page elements.
        let elements = unsafe { Owned(IOHIDDeviceCopyMatchingElements(native, matching.0, 0)) };
        if elements.0.is_null() {
            return;
        }
        // SAFETY: CopyMatchingElements returned a retained CFArray of live elements.
        let selected: HashMap<_, _> = unsafe {
            (0..CFArrayGetCount(elements.0))
                .filter_map(|index| {
                    let element = CFArrayGetValueAtIndex(elements.0, index);
                    if IOHIDElementGetType(element) != 2 || IOHIDElementGetUsagePage(element) != 9 {
                        return None;
                    }
                    let binding = NativeMouseButton {
                        vendor,
                        product,
                        usage: IOHIDElementGetUsage(element).try_into().ok()?,
                    };
                    binding.key()?;
                    if self
                        .wanted
                        .as_ref()
                        .is_some_and(|wanted| !wanted.contains(&binding))
                    {
                        return None;
                    }
                    Some((IOHIDElementGetCookie(element), element))
                })
                .collect()
        };
        if selected.is_empty() || access() != Access::Granted {
            return;
        }
        // SAFETY: this is a vetted physical pointer, permission checked immediately before opening.
        let result = unsafe { IOHIDDeviceOpen(native, 0) };
        if result != 0 {
            self.status = MonitorStatus::OpenFailed(result);
            tracing::warn!(
                vendor,
                product,
                result,
                "could not observe native mouse buttons"
            );
            return;
        }
        let elements = selected
            .into_iter()
            .map(|(cookie, element)| {
                let value = sample_value(native, element);
                let state = ButtonState::new(
                    value.map(|(down, _)| down),
                    value.map_or(CaptureInstant::now(), |(_, at)| at),
                );
                let binding = NativeMouseButton {
                    vendor,
                    product,
                    usage: unsafe { IOHIDElementGetUsage(element) as u16 },
                };
                (
                    cookie,
                    Element {
                        native: Owned(unsafe { CFRetain(element) }),
                        binding,
                        state,
                    },
                )
            })
            .collect();
        // SAFETY: retain the device so its elements live through removal; device callbacks are
        // scheduled only on this monitor's run loop, with this stable Box as context.
        unsafe {
            self.devices.insert(
                source,
                Device {
                    native: Owned(CFRetain(native)),
                    elements,
                },
            );
            IOHIDDeviceSetInputValueMatching(native, matching.0);
            IOHIDDeviceRegisterInputValueCallback(native, Some(value), (self as *mut Self).cast());
            IOHIDDeviceScheduleWithRunLoop(
                native,
                self.run_loop,
                super::super::kCFRunLoopCommonModes,
            );
        }
        self.status = MonitorStatus::Listening {
            devices: self.devices.len(),
        };
    }

    fn remove(&mut self, native: Ref) {
        // The disconnected service may no longer answer registry queries.
        let source = self.devices.iter().find_map(|(id, device)| {
            (unsafe { CFEqual(device.native.0, native) } != 0).then_some(*id)
        });
        let Some(source) = source else {
            return;
        };
        let Some(device) = self.devices.remove(&source) else {
            return;
        };
        self.close(device.native.0);
        (self.callback)(Event::Removed(source));
        self.status = MonitorStatus::Listening {
            devices: self.devices.len(),
        };
    }

    fn receive(&mut self, value: Ref) {
        if access() != Access::Granted {
            return;
        }
        // SAFETY: IOKit invokes this callback with a live value and element.
        let (native, element, cookie, down, at) = unsafe {
            let element = IOHIDValueGetElement(value);
            (
                IOHIDElementGetDevice(element),
                element,
                IOHIDElementGetCookie(element),
                IOHIDValueGetIntegerValue(value) != 0,
                value_time(value),
            )
        };
        let source = registry_id(native);
        let Some(device) = self.devices.get_mut(&source) else {
            return;
        };
        let Some(known) = device.elements.get_mut(&cookie) else {
            return;
        };
        if known.native.0 != element || !known.state.receive(down, at) {
            return;
        }
        (self.callback)(Event::Button(Edge {
            binding: known.binding,
            source,
            down,
            at,
        }));
    }

    pub fn sample(&mut self) -> Vec<Event> {
        let mut edges = Vec::new();
        for (source, device) in &mut self.devices {
            edges.extend(device.elements.values_mut().filter_map(|element| {
                if !element.state.needs_recovery() {
                    return None;
                }
                let (down, at) = sample_value(device.native.0, element.native.0)?;
                element.state.sample(Some(down), at).then_some(Edge {
                    binding: element.binding,
                    source: *source,
                    down: false,
                    at,
                })
            }));
        }
        edges.into_iter().map(Event::Button).collect()
    }

    fn close(&self, device: Ref) {
        // SAFETY: same context and run loop as registration, removed before releasing the device.
        unsafe {
            IOHIDDeviceRegisterInputValueCallback(
                device,
                None,
                (self as *const Self).cast_mut().cast(),
            );
            IOHIDDeviceUnscheduleFromRunLoop(
                device,
                self.run_loop,
                super::super::kCFRunLoopCommonModes,
            );
            IOHIDDeviceClose(device, 0);
        }
    }
}

impl Drop for Monitor {
    fn drop(&mut self) {
        // SAFETY: callbacks are unscheduled before their context and retained objects drop.
        unsafe {
            let context = (self as *mut Self).cast();
            IOHIDManagerRegisterDeviceMatchingCallback(self.manager.0, None, context);
            IOHIDManagerRegisterDeviceRemovalCallback(self.manager.0, None, context);
            IOHIDManagerUnscheduleFromRunLoop(
                self.manager.0,
                self.run_loop,
                super::super::kCFRunLoopCommonModes,
            );
        }
        self.devices
            .values()
            .for_each(|device| self.close(device.native.0));
    }
}

fn sample_value(device: Ref, element: Ref) -> Option<(bool, CaptureInstant)> {
    let mut value = ptr::null();
    // SAFETY: the monitor owns this opened device and its elements; value is borrowed.
    unsafe {
        if IOHIDDeviceGetValue(device, element, &raw mut value) != 0
            || value.is_null()
            || IOHIDValueGetTimeStamp(value) == 0
        {
            return None;
        }
        Some((IOHIDValueGetIntegerValue(value) != 0, value_time(value)))
    }
}

unsafe fn value_time(value: Ref) -> CaptureInstant {
    let ticks = unsafe { IOHIDValueGetTimeStamp(value) };
    if ticks == 0 {
        return CaptureInstant::now();
    }
    CaptureInstant::from_mach_ticks(ticks)
}

fn registry_id(device: Ref) -> u64 {
    let mut id = 0;
    // SAFETY: read-only query on a discovery/callback device, valid during this call.
    unsafe { IORegistryEntryGetRegistryEntryID(IOHIDDeviceGetService(device), &raw mut id) };
    id
}

pub(super) fn physical_pointer(device: Ref) -> bool {
    let transport = string(device, "Transport");
    if !matches!(
        transport.as_deref(),
        Some("USB" | "Bluetooth" | "Bluetooth Low Energy" | "SPI" | "I2C")
    ) || ["HIDVirtualDevice", "VirtualHIDDevice", "IOHIDVirtualDevice"]
        .iter()
        .any(|key| boolean(device, key))
    {
        return false;
    }
    // SAFETY: device is a live discovery result; these queries do not open it.
    unsafe { IOHIDDeviceConformsTo(device, 1, 1) || IOHIDDeviceConformsTo(device, 1, 2) }
}

unsafe extern "C" fn added(context: *mut c_void, result: i32, _: Ref, device: Ref) {
    if context.is_null() || result != 0 {
        return;
    }
    unsafe { &mut *context.cast::<Monitor>() }.add(device);
}
unsafe extern "C" fn removed(context: *mut c_void, _: i32, _: Ref, device: Ref) {
    if context.is_null() {
        return;
    }
    unsafe { &mut *context.cast::<Monitor>() }.remove(device);
}
unsafe extern "C" fn value(context: *mut c_void, result: i32, _: Ref, value: Ref) {
    if context.is_null() || result != 0 || value.is_null() {
        return;
    }
    unsafe { &mut *context.cast::<Monitor>() }.receive(value);
}
