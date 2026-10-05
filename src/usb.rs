use core::sync::atomic::{AtomicU8, Ordering};

use defmt::*;
use embassy_stm32::peripherals::USB_OTG_HS;
use embassy_usb::{
    class::hid::{HidBootProtocol, HidProtocolMode, HidSubclass, HidWriter, ReportId, State},
    control::OutResponse,
};
use usbd_hid::descriptor::SerializedDescriptor;
use {defmt_rtt as _, panic_probe as _};

use crate::{Irqs, descriptor::CapstanReport};

static HID_PROTOCOL_MODE: AtomicU8 = AtomicU8::new(HidProtocolMode::Boot as u8);

pub struct Buffers<'a> {
    ep_out_buffer: [u8; 2048],
    config_descriptor: [u8; 256],
    bos_descriptor: [u8; 256],
    control_buf: [u8; 64],
    request_handler: MyRequestHandler,
    state: embassy_usb::class::hid::State<'a>,
}
impl Buffers<'_> {
    pub fn new() -> Self {
        let ep_out_buffer = [0u8; 2048];
        // embassy-usb DeviceBuilder needs some buffers for building the descriptors.
        let config_descriptor = [0; 256];
        let bos_descriptor = [0; 256];
        let control_buf = [0; 64];

        let request_handler = MyRequestHandler {};
        let state = State::new();

        Buffers {
            ep_out_buffer,
            config_descriptor,
            bos_descriptor,
            control_buf,
            request_handler,
            state,
        }
    }
}

pub struct USB<'a> {
    pub device: embassy_usb::UsbDevice<'a, embassy_stm32::usb::Driver<'a, USB_OTG_HS>>,
    pub writer: embassy_usb::class::hid::HidWriter<
        'a,
        embassy_stm32::usb::Driver<'a, USB_OTG_HS>,
        { size_of::<CapstanReport>() + 1 },
    >,
}

impl<'a> USB<'a> {
    pub fn new<'b>(
        buffers: &'a mut Buffers<'b>,
        peripheral: embassy_stm32::Peri<'a, USB_OTG_HS>,
        dp: embassy_stm32::Peri<'a, impl embassy_stm32::usb::DpPin<USB_OTG_HS>>,
        dm: embassy_stm32::Peri<'a, impl embassy_stm32::usb::DmPin<USB_OTG_HS>>,
    ) -> Self
    where
        'a: 'b,
    {
        // Create the driver, from the HAL.
        let mut config = embassy_stm32::usb::Config::default();

        // Do not enable vbus_detection. This is a safe default that works in all boards.
        // However, if your USB device is self-powered (can stay powered on if USB is unplugged), you need
        // to enable vbus_detection to comply with the USB spec. If you enable it, the board
        // has to support it or USB won't work at all. See docs on `vbus_detection` for details.
        config.vbus_detection = false;

        let driver = embassy_stm32::usb::Driver::new_hs(
            peripheral,
            dp,
            dm,
            Irqs,
            &mut buffers.ep_out_buffer,
            config,
        );

        // Create embassy-usb Config
        let mut config = embassy_usb::Config::new(0xc0de, 0xcafe);
        config.manufacturer = Some("ACME");
        config.product = Some("Capstan");
        config.serial_number = Some("12345678");
        config.composite_with_iads = false;
        config.device_class = 0;
        config.device_sub_class = 0;
        config.device_protocol = 0;
        // config.supports_remote_wakeup = true; // TODO: Consider if we want this

        let mut builder = embassy_usb::Builder::new(
            driver,
            config,
            &mut buffers.config_descriptor,
            &mut buffers.bos_descriptor,
            &mut [], // no msos descriptors
            &mut buffers.control_buf,
        );

        // Create classes on the builder.
        let config = embassy_usb::class::hid::Config {
            report_descriptor: CapstanReport::desc(),
            request_handler: Some(&mut buffers.request_handler),
            poll_ms: 60,
            max_packet_size: 8,
            // hid_subclass: HidSubclass::No,
            hid_subclass: HidSubclass::Boot,
            hid_boot_protocol: HidBootProtocol::Mouse,
        };

        info!("HID Descriptor: {:#x}", CapstanReport::desc());

        let writer = HidWriter::<_, { size_of::<CapstanReport>() + 1 }>::new(
            &mut builder,
            &mut buffers.state,
            config,
        );

        // Build the builder.
        let device = builder.build();

        // Run the USB device.
        // let usb_fut = device.run();

        USB { device, writer }
    }
}

struct MyRequestHandler {}

impl embassy_usb::class::hid::RequestHandler for MyRequestHandler {
    // Section 7.2.1 of USB HID 1.11
    fn get_report(&mut self, id: ReportId, buf: &mut [u8]) -> Option<usize> {
        info!("Get report for {:?}", id);

        match id {
            ReportId::In(_) => None,
            ReportId::Out(_) => None,
            ReportId::Feature(0x02) => {
                // This is the feature we defined in the HID descriptor as being the Resolution Multipliers
                let report = CapstanReport::new(0);
                let report_length = match report.serialise_features(buf) {
                    Ok(v) => v as usize,
                    Err(_) => {
                        warn!("Report buffer overflowed when trying to serialise features");
                        return None;
                    }
                };
                Some(report_length + 1)
            }
            ReportId::Feature(_) => None,
        }
    }

    fn set_report(&mut self, id: ReportId, data: &[u8]) -> OutResponse {
        info!("Set report for {:?}: {=[u8]}", id, data);
        OutResponse::Accepted
    }

    fn get_protocol(&self) -> HidProtocolMode {
        let protocol = HidProtocolMode::from(HID_PROTOCOL_MODE.load(Ordering::Relaxed));
        info!("The current HID protocol mode is: {}", protocol);
        protocol
    }

    fn set_protocol(&mut self, protocol: HidProtocolMode) -> OutResponse {
        info!("Switching to HID protocol mode: {}", protocol);
        HID_PROTOCOL_MODE.store(protocol as u8, Ordering::Relaxed);
        OutResponse::Accepted
    }

    fn set_idle_ms(&mut self, id: Option<ReportId>, dur: u32) {
        info!("Set idle rate for {:?} to {:?}", id, dur);
    }

    fn get_idle_ms(&mut self, id: Option<ReportId>) -> Option<u32> {
        info!("Get idle rate for {:?}", id);
        None
    }
}
