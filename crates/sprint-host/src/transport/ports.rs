//! Serial port enumeration (never opens a port).

use std::cmp::Ordering;

use serialport::{SerialPortInfo, SerialPortType};

use super::PortInfo;

/// USB vendor IDs of USB-serial bridges and boards commonly found in 3D
/// printers, with a short name for each.
pub const PRINTER_USB_VENDORS: &[(u16, &str)] = &[
    (0x1A86, "WCH CH340/CH341"),
    (0x0403, "FTDI"),
    (0x10C4, "Silicon Labs CP210x"),
    (0x2341, "Arduino"),
    (0x2A03, "Arduino"),
    (0x2C99, "Prusa Research"),
    (0x0483, "STM32 virtual COM port"),
    (0x1D50, "OpenMoko / Klipper"),
    (0x16D0, "Duet"),
    (0x1EAF, "Maple"),
    (0x2E8A, "Raspberry Pi RP2040"),
    (0x303A, "Espressif"),
    (0x1FC9, "NXP / Smoothieboard"),
    (0x04D8, "Microchip"),
];

/// Whether a USB vendor ID belongs to a USB-serial bridge or board that
/// printers commonly use.
pub fn is_printer_vendor(vid: u16) -> bool {
    PRINTER_USB_VENDORS.iter().any(|(v, _)| *v == vid)
}

/// Enumerate serial ports (does not open them). Likely printers come first,
/// then the rest in natural order (`COM2` before `COM10`). Enumeration
/// errors yield an empty list.
pub fn list_ports() -> Vec<PortInfo> {
    match serialport::available_ports() {
        Ok(ports) => map_ports(ports),
        Err(e) => {
            log::warn!("could not enumerate serial ports: {e}");
            Vec::new()
        }
    }
}

/// Convert and sort enumerated ports.
pub(crate) fn map_ports(ports: Vec<SerialPortInfo>) -> Vec<PortInfo> {
    let mut out: Vec<PortInfo> = ports.into_iter().map(port_info).collect();
    out.sort_by(|a, b| {
        b.likely_printer
            .cmp(&a.likely_printer)
            .then_with(|| natural_cmp(&a.name, &b.name))
    });
    // The same port can be listed twice (e.g. once with USB details);
    // keep the first, most informative entry.
    let mut seen = std::collections::HashSet::new();
    out.retain(|p| seen.insert(p.name.clone()));
    out
}

fn port_info(p: SerialPortInfo) -> PortInfo {
    match p.port_type {
        SerialPortType::UsbPort(usb) => {
            let vendor = PRINTER_USB_VENDORS
                .iter()
                .find(|(v, _)| *v == usb.vid)
                .map(|(_, name)| *name);
            let description = usb
                .product
                .clone()
                .filter(|s| !s.trim().is_empty())
                .or_else(|| vendor.map(str::to_string))
                .unwrap_or_else(|| "USB serial device".to_string());
            PortInfo {
                name: p.port_name,
                description,
                vid: Some(usb.vid),
                pid: Some(usb.pid),
                manufacturer: usb.manufacturer.filter(|s| !s.trim().is_empty()),
                serial_number: usb.serial_number.filter(|s| !s.trim().is_empty()),
                likely_printer: vendor.is_some(),
            }
        }
        other => {
            let description = match other {
                SerialPortType::PciPort => "PCI serial port",
                SerialPortType::BluetoothPort => "Bluetooth serial port",
                _ => "Serial port",
            };
            PortInfo {
                name: p.port_name,
                description: description.to_string(),
                vid: None,
                pid: None,
                manufacturer: None,
                serial_number: None,
                likely_printer: false,
            }
        }
    }
}

/// Compare names so that embedded numbers sort numerically
/// (`COM2` < `COM10`, `ttyUSB2` < `ttyUSB10`), case-insensitively.
pub(crate) fn natural_cmp(a: &str, b: &str) -> Ordering {
    let (mut a, mut b) = (a.as_bytes(), b.as_bytes());
    loop {
        match (a.first(), b.first()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(x), Some(y)) if x.is_ascii_digit() && y.is_ascii_digit() => {
                let da = a.iter().take_while(|c| c.is_ascii_digit()).count();
                let db = b.iter().take_while(|c| c.is_ascii_digit()).count();
                let (na, nb) = (trim_zeros(&a[..da]), trim_zeros(&b[..db]));
                let ord = na.len().cmp(&nb.len()).then_with(|| na.cmp(nb));
                if ord != Ordering::Equal {
                    return ord;
                }
                a = &a[da..];
                b = &b[db..];
            }
            (Some(x), Some(y)) => {
                let ord = x.to_ascii_lowercase().cmp(&y.to_ascii_lowercase());
                if ord != Ordering::Equal {
                    return ord;
                }
                a = &a[1..];
                b = &b[1..];
            }
        }
    }
}

fn trim_zeros(digits: &[u8]) -> &[u8] {
    let zeros = digits.iter().take_while(|&&c| c == b'0').count();
    &digits[zeros.min(digits.len().saturating_sub(1))..]
}

#[cfg(test)]
mod tests {
    use super::*;
    use serialport::UsbPortInfo;

    fn usb(name: &str, vid: u16, product: Option<&str>) -> SerialPortInfo {
        SerialPortInfo {
            port_name: name.to_string(),
            port_type: SerialPortType::UsbPort(UsbPortInfo {
                vid,
                pid: 0x7523,
                serial_number: Some("A1".into()),
                manufacturer: Some("  ".into()),
                product: product.map(str::to_string),
            }),
        }
    }

    #[test]
    fn known_vendors_are_likely_printers_and_come_first() {
        let ports = vec![
            SerialPortInfo {
                port_name: "COM1".into(),
                port_type: SerialPortType::PciPort,
            },
            usb("COM10", 0x1A86, None),
            usb("COM9", 0x046D, Some("Some Mouse Thing")),
            usb("COM2", 0x2C99, Some("Original Prusa i3 MK3")),
            SerialPortInfo {
                port_name: "COM5".into(),
                port_type: SerialPortType::BluetoothPort,
            },
            SerialPortInfo {
                port_name: "COM5".into(),
                port_type: SerialPortType::Unknown,
            },
        ];
        let out = map_ports(ports);
        let names: Vec<&str> = out.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["COM2", "COM10", "COM1", "COM5", "COM9"]);
        assert!(out[0].likely_printer && out[1].likely_printer);
        assert!(!out[2].likely_printer);
        assert_eq!(out[0].description, "Original Prusa i3 MK3");
        assert_eq!(out[1].description, "WCH CH340/CH341");
        assert_eq!(out[1].vid, Some(0x1A86));
        assert_eq!(out[1].manufacturer, None, "blank strings are dropped");
        assert_eq!(out[1].serial_number.as_deref(), Some("A1"));
        assert_eq!(out[2].description, "PCI serial port");
        assert_eq!(out[3].description, "Bluetooth serial port");
        assert_eq!(out[4].description, "Some Mouse Thing");
    }

    #[test]
    fn a_port_listed_twice_appears_once() {
        // Sorting puts the USB entries first, so the plain COM5 ends up
        // behind COM9, not next to the USB COM5.
        let ports = vec![
            SerialPortInfo {
                port_name: "COM5".into(),
                port_type: SerialPortType::Unknown,
            },
            usb("COM9", 0x0403, None),
            usb("COM5", 0x1A86, None),
        ];
        let out = map_ports(ports);
        let names: Vec<&str> = out.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["COM5", "COM9"]);
        assert!(out[0].likely_printer && out[0].vid == Some(0x1A86));
    }

    #[test]
    fn every_listed_vendor_counts() {
        for vid in [
            0x1A86, 0x0403, 0x10C4, 0x2341, 0x2A03, 0x2C99, 0x0483, 0x1D50, 0x16D0, 0x1EAF, 0x2E8A,
            0x303A, 0x1FC9, 0x04D8,
        ] {
            assert!(is_printer_vendor(vid), "{vid:04X}");
        }
        assert!(!is_printer_vendor(0x046D));
    }

    #[test]
    fn natural_ordering() {
        let mut names = vec![
            "COM10",
            "com2",
            "COM1",
            "/dev/ttyUSB10",
            "/dev/ttyUSB2",
            "COM02",
        ];
        names.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(
            names,
            [
                "/dev/ttyUSB2",
                "/dev/ttyUSB10",
                "COM1",
                "com2",
                "COM02",
                "COM10"
            ]
        );
        assert_eq!(natural_cmp("", ""), Ordering::Equal);
        assert_eq!(natural_cmp("a", ""), Ordering::Greater);
        assert_eq!(natural_cmp("x00", "x0"), Ordering::Equal);
    }

    #[test]
    fn listing_real_ports_does_not_panic() {
        // Enumeration only; nothing is opened.
        let _ = list_ports();
    }
}
