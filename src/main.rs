#![no_std]
#![no_main]

use crate::descriptor::CapstanReport;
use core::fmt::Write;
use core::sync::atomic::{AtomicU8, Ordering};
use defmt::*;
use embassy_executor::Spawner;
use embassy_futures::join::join;
use embassy_stm32::exti::ExtiInput;
use embassy_stm32::gpio::{Level, Output, Pull, Speed};
use embassy_stm32::usart::Uart;
use embassy_stm32::usb::Driver;
use embassy_stm32::{Config, bind_interrupts, dma, exti, interrupt, peripherals, usart, usb};
use embassy_sync::blocking_mutex::raw::ThreadModeRawMutex;
use embassy_sync::channel::{Channel, Sender};
use embassy_usb::Builder;
use embassy_usb::class::hid::{
    HidBootProtocol, HidProtocolMode, HidSubclass, HidWriter, ReportId, RequestHandler, State,
};
use embassy_usb::control::OutResponse;
use heapless::String;
use usbd_hid::descriptor::{MouseReport, SerializedDescriptor};

use {defmt_rtt as _, panic_probe as _};

mod descriptor;
mod isr;

// Map to the interrupt vectors in the startup script after removing `_IRQHandler`
// Interrupts are called in the order they appear here
bind_interrupts!(struct Irqs {
    USART1 => usart::InterruptHandler<peripherals::USART1>;
    GPDMA1_CHANNEL0 => dma::InterruptHandler<peripherals::GPDMA1_CH0>;
    GPDMA1_CHANNEL1 => dma::InterruptHandler<peripherals::GPDMA1_CH1>;
    USB_OTG_HS => usb::InterruptHandler<peripherals::USB_OTG_HS>;
    EXTI13 => exti::InterruptHandler<interrupt::typelevel::EXTI13>; // B1
    EXTI5 => exti::InterruptHandler<interrupt::typelevel::EXTI5>; // B2
    EXTI4 => isr::CustomISR<interrupt::typelevel::EXTI4>; // B3
});

static HID_PROTOCOL_MODE: AtomicU8 = AtomicU8::new(HidProtocolMode::Boot as u8);

#[derive(Debug, Clone, Copy)]
enum ScrollDirection {
    Up,
    Down,
}
static CHANNEL: Channel<ThreadModeRawMutex, ScrollDirection, 64> = Channel::new();

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    let mut config = Config::default();
    {
        use embassy_stm32::rcc::*;
        // Enable external oscillator
        config.rcc.hse = Some(Hse {
            prescaler: embassy_stm32::rcc::HsePrescaler::Div1,
            trim: None,
        });
        // Route external oscilator to the USB peripheral
        config.rcc.mux.otghssel = mux::Otghssel::Hse;
        // Set up Sysclock
        config.rcc.sys = Sysclk::Pll1R;
        config.rcc.pll1 = Some(Pll {
            source: PllSource::Hse,
            prediv: PllPreDiv::Div2,   // PLLM = 2 → HSE / 2 = 16 MHz
            mul: PllMul::Mul30,        // PLLN = 30 → 16 MHz * 30 = 480 MHz VCO
            divr: Some(PllDiv::Div5),  // PLLR = 5 → 96 MHz (Sysclk)
            divq: Some(PllDiv::Div10), // PLLQ = 10 → 48 MHz
            divp: Some(PllDiv::Div30), // PLLP = 30 → 16 MHz
            frac: Some(0),             // Fractional part (disabled)
        });
    }
    let p = embassy_stm32::init(config);
    info!("Hello World!");

    let uart_config = usart::Config::default();
    // let mut uart = Uart::new_blocking(p.USART1, p.PA8, p.PB14, uart_config).unwrap();
    let mut usart = Uart::new(
        p.USART1,
        p.PA8,
        p.PB14,
        p.GPDMA1_CH0,
        p.GPDMA1_CH1,
        Irqs,
        uart_config,
    )
    .unwrap();

    // LD2 - PC4
    let mut ld1 = Output::new(p.PD8, Level::High, Speed::Low);
    let mut ld2 = Output::new(p.PC4, Level::High, Speed::Low);
    let mut ld3 = Output::new(p.PB8, Level::High, Speed::Low);

    let mut b1 = ExtiInput::new(p.PC13, p.EXTI13, Pull::Up, Irqs);
    let mut b2 = ExtiInput::new(p.PC5, p.EXTI5, Pull::Up, Irqs);
    // let mut b3 = ExtiInput::new(p.PB4, p.EXTI4, Pull::Up, Irqs);

    // Create the driver, from the HAL.
    let mut ep_out_buffer = [0u8; 2048];
    let mut config = embassy_stm32::usb::Config::default();

    // Do not enable vbus_detection. This is a safe default that works in all boards.
    // However, if your USB device is self-powered (can stay powered on if USB is unplugged), you need
    // to enable vbus_detection to comply with the USB spec. If you enable it, the board
    // has to support it or USB won't work at all. See docs on `vbus_detection` for details.
    config.vbus_detection = false;

    let driver = Driver::new_hs(p.USB_OTG_HS, Irqs, p.PD6, p.PD7, &mut ep_out_buffer, config);

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

    // Create embassy-usb DeviceBuilder using the driver and config.
    // It needs some buffers for building the descriptors.
    let mut config_descriptor = [0; 256];
    let mut bos_descriptor = [0; 256];
    let mut control_buf = [0; 64];

    let mut request_handler = MyRequestHandler {};
    let mut state = State::new();
    let mut builder = Builder::new(
        driver,
        config,
        &mut config_descriptor,
        &mut bos_descriptor,
        &mut [], // no msos descriptors
        &mut control_buf,
    );

    // Create classes on the builder.
    let config = embassy_usb::class::hid::Config {
        report_descriptor: CapstanReport::desc(),
        request_handler: Some(&mut request_handler),
        poll_ms: 60,
        max_packet_size: 8,
        // hid_subclass: HidSubclass::No,
        hid_subclass: HidSubclass::Boot,
        hid_boot_protocol: HidBootProtocol::Mouse,
    };

    info!("HID Descriptor: {:#x}", CapstanReport::desc());

    let mut writer =
        HidWriter::<_, { size_of::<CapstanReport>() + 1 }>::new(&mut builder, &mut state, config);

    // Build the builder.
    let mut usb = builder.build();

    // Run the USB device.
    let usb_fut = usb.run();

    let mut tx: String<128> = String::new();
    core::write!(&mut tx, "Hello DMA World!\n").unwrap();
    usart.write(tx.as_bytes()).await.ok();

    spawner.spawn(unwrap!(watch_button(
        CHANNEL.sender(),
        b1,
        ScrollDirection::Up
    )));
    spawner.spawn(unwrap!(watch_button(
        CHANNEL.sender(),
        b2,
        ScrollDirection::Down
    )));

    // Do stuff with the class!
    let hid_fut = async {
        const SCROLL_AMOUNT: i8 = 1;
        loop {
            match CHANNEL.receive().await {
                ScrollDirection::Up => {
                    let report = CapstanReport::new(SCROLL_AMOUNT);
                    match writer.write_serialize(&report).await {
                        Ok(()) => {}
                        Err(e) => warn!("Failed to send report: {:?}", e),
                    }
                }
                ScrollDirection::Down => {
                    let report = CapstanReport::new(-SCROLL_AMOUNT);
                    match writer.write_serialize(&report).await {
                        Ok(()) => {}
                        Err(e) => warn!("Failed to send report: {:?}", e),
                    }
                }
            }
        }
    };

    // Run everything concurrently.
    // If we had made everything `'static` above instead, we could do this using separate tasks instead.
    join(usb_fut, hid_fut).await;

    // Print with blocking UART
    // unwrap!(usart.blocking_write(b"Hello Embassy World!\n"));

    // // Print with DMA
    // let mut tx: String<128> = String::new();
    // core::write!(&mut tx, "Hello DMA World!\n").unwrap();
    // usart.write(tx.as_bytes()).await.ok();

    // loop {
    //     info!("led on!");
    //     led.set_high();
    //     Timer::after_millis(500).await;

    //     info!("led off!");
    //     led.set_low();
    //     Timer::after_millis(500).await;
    // }
}

// A pool size of 2 means you can spawn two instances of this task.
#[embassy_executor::task(pool_size = 2)]
async fn watch_button(
    control: Sender<'static, ThreadModeRawMutex, ScrollDirection, 64>,
    mut button: ExtiInput<'static, embassy_stm32::mode::Async>,
    direction: ScrollDirection,
) {
    loop {
        button.wait_for_falling_edge().await;
        control.send(direction).await;
    }
}

// #[interrupt]
// fn handle_b3(){

// }
//

struct MyRequestHandler {}

impl RequestHandler for MyRequestHandler {
    // Section 7.2.1 of USB HID 1.11
    fn get_report(&mut self, id: ReportId, buf: &mut [u8]) -> Option<usize> {
        info!("Get report for {:?}", id);

        match id {
            ReportId::In(_) => None,
            ReportId::Out(_) => None,
            ReportId::Feature(0x02) => {
                buf[0] = 0x02;
                // This is the feature we defined in the HID descriptor as being the Resolution Multipliers
                let report = CapstanReport::new(0);
                let report_length = match report.serialise_features(&mut buf[1..]) {
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
