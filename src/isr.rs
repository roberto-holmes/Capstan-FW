use core::marker::PhantomData;

use embassy_stm32::exti::TriggerEdge;
use embassy_stm32::gpio::Input;
use embassy_stm32::interrupt;
use embassy_stm32::interrupt::typelevel::Interrupt as InterruptType;

pub fn _setup(_pin: &Input, _trigger_edge: TriggerEdge) {
    // TODO: Enable the interrupt on the pin for the given edge
}

pub struct CustomISR<T: crate::interrupt::typelevel::Interrupt> {
    _marker: PhantomData<T>,
}

impl<T: InterruptType> interrupt::typelevel::Handler<T> for CustomISR<T> {
    unsafe fn on_interrupt() {
        // Toggle LED 3
    }
}
