#![no_std]
#![no_main]

use crate::ble::BLE;
use crate::descriptor::CapstanReport;
use crate::usb::USB;
use core::fmt::Write;
use defmt::*;
use embassy_executor::Spawner;
use embassy_futures::join::join;
use embassy_futures::select::{Either3, select3};
use embassy_stm32;
use embassy_stm32::{Config, bind_interrupts, interrupt, usart};
use embassy_sync::blocking_mutex::raw::ThreadModeRawMutex;
use embassy_sync::channel::{Channel, Sender};
use embassy_time::{Duration, Ticker};
use heapless::String;

use {defmt_rtt as _, panic_probe as _};

mod ble;
mod descriptor;
mod isr;
mod services;
mod usb;

// Map to the interrupt vectors in the startup script after removing `_IRQHandler`
// Interrupts are called in the order they appear here
bind_interrupts!(pub struct Irqs {
    RADIO => embassy_stm32_wpan::HighInterruptHandler;
    HASH => embassy_stm32_wpan::LowInterruptHandler;
    USART1 => embassy_stm32::usart::InterruptHandler<embassy_stm32::peripherals::USART1>;
    GPDMA1_CHANNEL0 => embassy_stm32::dma::InterruptHandler<embassy_stm32::peripherals::GPDMA1_CH0>;
    GPDMA1_CHANNEL1 => embassy_stm32::dma::InterruptHandler<embassy_stm32::peripherals::GPDMA1_CH1>;
    USB_OTG_HS => embassy_stm32::usb::InterruptHandler<embassy_stm32::peripherals::USB_OTG_HS>;
    EXTI13 => embassy_stm32::exti::InterruptHandler<embassy_stm32::interrupt::typelevel::EXTI13>; // B1
    EXTI5 => embassy_stm32::exti::InterruptHandler<embassy_stm32::interrupt::typelevel::EXTI5>; // B2
    EXTI4 => crate::isr::CustomISR<interrupt::typelevel::EXTI4>; // B3
});

#[derive(Debug, Clone, Copy, PartialEq)]
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
        config.rcc = Config::new_wpan();
        config.rcc.mux.otghssel = mux::Otghssel::Hse; // Route external oscilator to the USB peripheral
    }
    let p = embassy_stm32::init(config);
    info!("Hello World!");

    let is_wireless = {
        // TODO: Read GPIO to decide whether to spin up BLE or USB stack
        true
    };

    let uart_config = usart::Config::default();
    // let mut uart = Uart::new_blocking(p.USART1, p.PA8, p.PB14, uart_config).unwrap();
    let mut usart = embassy_stm32::usart::Uart::new(
        p.USART1,
        p.PB14,
        p.PA8,
        p.GPDMA1_CH0,
        p.GPDMA1_CH1,
        Irqs,
        uart_config,
    )
    .unwrap();

    // LD2 - PC4
    //     let mut ld1 = Output::new(p.PD8, Level::High, Speed::Low);
    //     let mut ld2 = Output::new(p.PC4, Level::High, Speed::Low);
    //     let mut ld3 = Output::new(p.PB8, Level::High, Speed::Low);
    //
    let (b1, b2) = {
        use embassy_stm32::exti::ExtiInput;
        use embassy_stm32::gpio::*;
        (
            ExtiInput::new(p.PC13, p.EXTI13, Pull::Up, Irqs),
            ExtiInput::new(p.PC5, p.EXTI5, Pull::Up, Irqs),
        )
    };
    // let mut b3 = ExtiInput::new(p.PB4, p.EXTI4, Pull::Up, Irqs);

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

    if is_wireless {
        let mut ble = BLE::new(&spawner).await;
        ble.advertise().await;

        let mut ticker = Ticker::every(Duration::from_secs(1));
        loop {
            match select3(ble.read_event(), ticker.next(), CHANNEL.receive()).await {
                Either3::First(event) => ble.process_event(event).await,
                Either3::Second(_) => ble.update_battery().await,
                Either3::Third(direction) => {
                    ble.scroll(if direction == ScrollDirection::Up {
                        1
                    } else {
                        -1
                    })
                    .await
                }
            }
        }
    } else {
        let mut usb_buffers = crate::usb::Buffers::new();
        let mut usb = USB::new(&mut usb_buffers, p.USB_OTG_HS, p.PD6, p.PD7);

        let mut tx: String<128> = String::new();
        core::write!(&mut tx, "Hello DMA World!\n").unwrap();
        usart.write(tx.as_bytes()).await.ok();

        // Do stuff with the class!
        let hid_fut = async {
            const SCROLL_AMOUNT: i8 = 1;
            loop {
                match CHANNEL.receive().await {
                    ScrollDirection::Up => {
                        let report = CapstanReport::new(SCROLL_AMOUNT);
                        match usb.writer.write_serialize(&report).await {
                            Ok(()) => {}
                            Err(e) => warn!("Failed to send report: {:?}", e),
                        }
                    }
                    ScrollDirection::Down => {
                        let report = CapstanReport::new(-SCROLL_AMOUNT);
                        match usb.writer.write_serialize(&report).await {
                            Ok(()) => {}
                            Err(e) => warn!("Failed to send report: {:?}", e),
                        }
                    }
                }
            }
        };

        // Run everything concurrently.
        // If we had made everything `'static` above instead, we could do this using separate tasks instead.
        join(usb.device.run(), hid_fut).await;
    }

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
    mut button: embassy_stm32::exti::ExtiInput<'static, embassy_stm32::mode::Async>,
    direction: ScrollDirection,
) {
    loop {
        button.wait_for_falling_edge().await;
        control.send(direction).await;
    }
}
