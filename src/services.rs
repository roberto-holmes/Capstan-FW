use embassy_stm32_wpan::bluetooth::{
    error::BleError,
    gatt::{
        AttributeAccess, CharProperties, CharacteristicHandle, DescriptorHandle, GattEventMask,
        GattServer, SecurityPermissions, ServiceHandle, ServiceType, Uuid,
    },
};
use usbd_hid::descriptor::{AsInputReport, SerializedDescriptor};

use crate::descriptor::{self, CapstanReport};

pub const HID_SERVICE_UUID: u16 = 0x1812; // Human Interface Device
const HID_INFO_CHAR_UUID: u16 = 0x2A4A;
const REPORT_MAP_CHAR_UUID: u16 = 0x2A4B;
const HID_CONTROL_POINT_CHAR_UUID: u16 = 0x2A4C;
const REPORT_CHAR_UUID: u16 = 0x2A4D;

const DIS_SERVICE_UUID: u16 = 0x180A; // Device Information Service
#[allow(non_upper_case_globals)]
const PnP_ID_CHAR_UUID: u16 = 0x2A50;

const BATTERY_SERVICE_UUID: u16 = 0x180F; // Battery Service
const BATTERY_LEVEL_CHAR_UUID: u16 = 0x2A19;

const REPORT_REFERENCE_UUID: u16 = 0x2908;

#[allow(dead_code)]
struct ReportCharacteristic {
    char_handle: CharacteristicHandle,
    report_reference_handle: DescriptorHandle,
}

// Human Interface Device Service
// https://www.bluetooth.com/specifications/specs/html/?src=hids_v1-0_1765320236/HIDS_v1.0/out/en/index-en.html
#[allow(dead_code)]
pub struct HidService {
    service_handle: ServiceHandle,
    // TODO: Add a feature report as well as this input report
    input_report_char: ReportCharacteristic,
    feature_report_char: ReportCharacteristic,
    report_map_char_handle: CharacteristicHandle,
    hid_info_char_handle: CharacteristicHandle,
    hid_control_point_char_handle: CharacteristicHandle,
    pub notifications_enabled: bool,
    // report_map: [u8; 60],
    // report: MouseReport,
    // report_buff: [u8; size_of::<MouseReport>()],
}

impl HidService {
    fn setup_input_report(
        gatt: &mut GattServer,
        service_handle: ServiceHandle,
    ) -> ReportCharacteristic {
        let char_handle = gatt
            .add_characteristic(
                service_handle,
                Uuid::from_u16(REPORT_CHAR_UUID),
                descriptor::INPUT_REPORT_SIZE as u16,
                CharProperties::READ | CharProperties::WRITE | CharProperties::NOTIFY,
                SecurityPermissions::NONE,
                GattEventMask::NONE,
                0,
                true,
            )
            .expect("Failed to add Report characteristic");

        // 2.5.3.2
        let report_reference = [
            0x00, // Report ID: Zero when there is only one instance of the Report characteristic for any given Report Type.
            0x01, // Input Report
        ];
        let report_reference_handle = gatt
            .add_descriptor(
                service_handle,
                char_handle,
                Uuid::from_u16(REPORT_REFERENCE_UUID),
                report_reference.len() as u8,
                &report_reference,
                SecurityPermissions::NONE,
                AttributeAccess::READ,
                GattEventMask::NONE,
                0,
                false,
            )
            .expect("Failed to add Client Characteristic Configuration Descriptor");

        let report = CapstanReport::new(0);

        let mut report_buff = [0x00; descriptor::INPUT_REPORT_SIZE];
        report
            .serialize(&mut report_buff)
            .expect("Failed to serialise mouse report");
        gatt.update_characteristic_value(service_handle, char_handle, 0, &report_buff)
            .expect("Failed to set Report value");

        ReportCharacteristic {
            char_handle,
            report_reference_handle,
        }
    }
    fn setup_feature_report(
        gatt: &mut GattServer,
        service_handle: ServiceHandle,
    ) -> ReportCharacteristic {
        let char_handle = gatt
            .add_characteristic(
                service_handle,
                Uuid::from_u16(REPORT_CHAR_UUID),
                descriptor::FEATURE_REPORT_SIZE as u16,
                CharProperties::READ | CharProperties::WRITE,
                SecurityPermissions::NONE,
                GattEventMask::NONE,
                0,
                true,
            )
            .expect("Failed to add Report characteristic");

        // 2.5.3.2
        let report_reference = [
            0x00, // Report ID: Zero when there is only one instance of the Report characteristic for any given Report Type.
            0x03, // Feature Report
        ];
        let report_reference_handle = gatt
            .add_descriptor(
                service_handle,
                char_handle,
                Uuid::from_u16(REPORT_REFERENCE_UUID),
                report_reference.len() as u8,
                &report_reference,
                SecurityPermissions::NONE,
                AttributeAccess::READ,
                GattEventMask::NONE,
                0,
                false,
            )
            .expect("Failed to add Client Characteristic Configuration Descriptor");

        let report = CapstanReport::new(0);

        let mut report_buff = [0x00; descriptor::FEATURE_REPORT_SIZE];
        report
            .serialise_features(&mut report_buff)
            .expect("Failed to serialise mouse report");
        gatt.update_characteristic_value(service_handle, char_handle, 0, &report_buff)
            .expect("Failed to set Report value");

        ReportCharacteristic {
            char_handle,
            report_reference_handle,
        }
    }

    fn setup_report_map(
        gatt: &mut GattServer,
        service_handle: ServiceHandle,
    ) -> CharacteristicHandle {
        let char_handle = gatt
            .add_characteristic(
                service_handle,
                Uuid::from_u16(REPORT_MAP_CHAR_UUID),
                256,
                CharProperties::READ,
                SecurityPermissions::NONE,
                GattEventMask::empty(),
                0,
                true,
            )
            .expect("Failed to add Report Map characteristic");

        gatt.update_characteristic_value(service_handle, char_handle, 0, CapstanReport::desc())
            .expect("Failed to set Report Map value");

        char_handle
    }

    fn setup_info(gatt: &mut GattServer, service_handle: ServiceHandle) -> CharacteristicHandle {
        let char_handle = gatt
            .add_characteristic(
                service_handle,
                Uuid::from_u16(HID_INFO_CHAR_UUID),
                4,
                CharProperties::READ,
                SecurityPermissions::NONE,
                GattEventMask::empty(),
                0,
                false,
            )
            .expect("Failed to add HID Information characteristic");

        // HID Service Specification - 2.10.2 HID Information Characteristic Value
        let info_buff = [
            0x01, 0x01, // bcdHID: USB HID version 1.1
            0x00, // bCountryCode: Not localised
            0x03, // Flags: Remote wake + normally connectable
        ];

        gatt.update_characteristic_value(service_handle, char_handle, 0, &info_buff)
            .expect("Failed to set HID info value");

        char_handle
    }

    fn setup_control_point(
        gatt: &mut GattServer,
        service_handle: ServiceHandle,
    ) -> CharacteristicHandle {
        let char_handle = gatt
            .add_characteristic(
                service_handle,
                Uuid::from_u16(HID_CONTROL_POINT_CHAR_UUID),
                1,
                CharProperties::WRITE_WITHOUT_RESPONSE,
                SecurityPermissions::NONE,
                GattEventMask::empty(),
                0,
                false,
            )
            .expect("Failed to add HID Control Point characteristic");

        char_handle
    }

    pub fn setup(gatt: &mut GattServer) -> Self {
        let service_handle = gatt
            .add_service(Uuid::from_u16(HID_SERVICE_UUID), ServiceType::Primary, 20)
            .expect("Failed to add HID service");

        let input_report_char = Self::setup_input_report(gatt, service_handle);
        let feature_report_char = Self::setup_feature_report(gatt, service_handle);
        let report_map_char_handle = Self::setup_report_map(gatt, service_handle);
        let hid_info_char_handle = Self::setup_info(gatt, service_handle);
        let hid_control_point_char_handle = Self::setup_control_point(gatt, service_handle);

        return Self {
            service_handle,
            input_report_char,
            feature_report_char,
            report_map_char_handle,
            hid_info_char_handle,
            hid_control_point_char_handle,
            notifications_enabled: false,
        };
    }
    pub fn report_handle(&self) -> u16 {
        self.input_report_char.char_handle.0
    }
    pub fn report_map_handle(&self) -> u16 {
        self.report_map_char_handle.0
    }
    pub fn info_handle(&self) -> u16 {
        self.hid_info_char_handle.0
    }
    pub fn control_point_handle(&self) -> u16 {
        self.hid_control_point_char_handle.0
    }
    pub fn scroll(
        &self,
        gatt: &mut GattServer,
        conn_handle: u16,
        amount: i8,
    ) -> Result<(), BleError> {
        if !self.notifications_enabled {
            return Ok(());
        }
        let report = CapstanReport::new(amount);
        let mut report_buff = [0x00; descriptor::INPUT_REPORT_SIZE];
        report
            .serialize(&mut report_buff)
            .expect("Failed to serialise mouse report");

        gatt.notify(
            conn_handle,
            self.service_handle,
            self.input_report_char.char_handle,
            &report_buff,
        )
    }
}

// Device Information Service
#[allow(dead_code)]
pub struct DisService {
    service_handle: ServiceHandle,
    pnp_id_char_handle: CharacteristicHandle,
}

impl DisService {
    pub fn setup(
        gatt: &mut GattServer,
        vendor_id_source: u8,
        vendor_id: u16,
        product_id: u16,
        version: u16,
    ) -> Self {
        let service_handle = gatt
            .add_service(Uuid::from_u16(DIS_SERVICE_UUID), ServiceType::Primary, 3)
            .expect("Failed to add DIS service");

        let pnp_id_char_handle = gatt
            .add_characteristic(
                service_handle,
                Uuid::from_u16(PnP_ID_CHAR_UUID),
                7,
                CharProperties::READ,
                SecurityPermissions::NONE,
                GattEventMask::empty(),
                0,
                false,
            )
            .expect("Failed to add PnP ID characteristic");

        let packet = [
            vendor_id_source,
            vendor_id as u8,
            (vendor_id >> 8) as u8,
            product_id as u8,
            (product_id >> 8) as u8,
            version as u8,
            (version >> 8) as u8,
        ];
        gatt.update_characteristic_value(service_handle, pnp_id_char_handle, 0, &packet)
            .expect("Failed to set PnP ID value");
        return Self {
            service_handle,
            pnp_id_char_handle,
        };
    }
}

// Battery Service
// https://www.bluetooth.com/specifications/specs/bas-1-1/
#[allow(dead_code)]
pub struct BasService {
    pub service_handle: ServiceHandle,
    pub battery_level_char_handle: CharacteristicHandle,
    pub notifications_enabled: bool,
    battery_level: u8,
}

impl BasService {
    pub fn setup(gatt: &mut GattServer, battery_level: u8) -> Self {
        let service_handle = gatt
            .add_service(
                Uuid::from_u16(BATTERY_SERVICE_UUID),
                ServiceType::Primary,
                4,
            )
            .expect("Failed to add BAS service");

        let battery_level_char_handle = gatt
            .add_characteristic(
                service_handle,
                Uuid::from_u16(BATTERY_LEVEL_CHAR_UUID),
                1,
                CharProperties::READ | CharProperties::NOTIFY,
                SecurityPermissions::NONE,
                GattEventMask::empty(),
                0,
                false,
            )
            .expect("Failed to add Battery Level characteristic");

        gatt.update_characteristic_value(
            service_handle,
            battery_level_char_handle,
            0,
            &[battery_level],
        )
        .expect("Failed to set Report Map value");

        return Self {
            service_handle,
            battery_level_char_handle,
            notifications_enabled: false,
            battery_level,
        };
    }
    pub fn batt_level_handle(&self) -> u16 {
        self.battery_level_char_handle.0
    }
    pub fn update(&mut self, gatt: &mut GattServer, conn_handle: u16) -> Result<(), BleError> {
        self.battery_level += 1;
        if self.battery_level > 100 {
            self.battery_level = 50;
        }
        if self.notifications_enabled {
            gatt.notify(
                conn_handle,
                self.service_handle,
                self.battery_level_char_handle,
                &[self.battery_level],
            )
        } else {
            Ok(())
        }
    }
}
