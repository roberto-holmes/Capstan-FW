### Debugging

Build and flash debug with `cargo run`
Build and flash release with `cargo run -r`

Attach to the RTT output of a running board:
`probe-rs attach --chip STM32WBA65RI .\target\thumbv8m.main-none-eabihf\debug\capstan-rs`
(It needs the binary to know where to find debug symbols)
